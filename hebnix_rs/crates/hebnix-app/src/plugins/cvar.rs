use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

use crate::plugins::store::PluginStore;

#[derive(Debug, Clone, PartialEq)]
pub enum CvarValue {
    String(String),
    Integer(i64),
    Number(f64),
}

impl fmt::Display for CvarValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::String(value) => write!(f, "{value:?}"),
            Self::Integer(value) => write!(f, "{value}"),
            Self::Number(value) => write!(f, "{value}"),
        }
    }
}

#[derive(Debug, Clone)]
struct CvarEntry {
    owner: String,
    value: Option<CvarValue>,
    store: Rc<RefCell<PluginStore>>,
}

#[derive(Debug, Clone, Default)]
pub struct CvarRegistry {
    entries: HashMap<String, CvarEntry>,
}

impl CvarRegistry {
    pub fn register(
        &mut self,
        name: &str,
        owner: &str,
        store: Rc<RefCell<PluginStore>>,
    ) -> Result<(), String> {
        validate_name(name)?;
        if let Some(existing) = self.entries.get(name) {
            return Err(format!(
                "cvar '{name}' is already registered by plugin '{}'",
                existing.owner
            ));
        }
        let value = store.borrow().get_cvar(name);
        self.entries.insert(
            name.to_string(),
            CvarEntry {
                owner: owner.to_string(),
                value,
                store,
            },
        );
        Ok(())
    }

    pub fn set(&mut self, name: &str, value: CvarValue) -> Result<(), String> {
        let entry = self
            .entries
            .get_mut(name)
            .ok_or_else(|| format!("cvar '{name}' has not been registered by a plugin"))?;
        entry.store.borrow_mut().set_cvar(name, &value);
        entry.value = Some(value);
        Ok(())
    }

    pub fn delete(&mut self, name: &str, owner: &str) -> Result<(), String> {
        let entry = self
            .entries
            .get(name)
            .ok_or_else(|| format!("cvar '{name}' has not been registered by a plugin"))?;
        if entry.owner != owner {
            return Err(format!(
                "cvar '{name}' is owned by plugin '{}' and cannot be deleted by '{owner}'",
                entry.owner
            ));
        }
        entry.store.borrow_mut().delete_cvar(name);
        self.entries.remove(name);
        Ok(())
    }

    /// `None` means the cvar is not registered. `Some(None)` is registered but
    /// has not been assigned a value yet.
    pub fn get(&self, name: &str) -> Option<Option<CvarValue>> {
        self.entries.get(name).map(|entry| entry.value.clone())
    }

    pub fn unregister_owner(&mut self, owner: &str) {
        self.entries.retain(|_, entry| entry.owner != owner);
    }
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("cvar name cannot be empty".to_string());
    }
    if name.trim() != name {
        return Err("cvar names cannot start or end with whitespace".to_string());
    }
    if name.chars().any(char::is_whitespace) {
        return Err("cvar names cannot contain whitespace".to_string());
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConsoleCvarCommand {
    Get { name: String },
    Set { name: String, value: CvarValue },
}

pub fn parse_console_command(raw: &str) -> Result<ConsoleCvarCommand, String> {
    let raw = raw.trim();
    let (command, rest) = split_once_whitespace(raw).unwrap_or((raw, ""));
    if !command.eq_ignore_ascii_case("cvar") {
        return Err("expected the cvar command".to_string());
    }

    let rest = rest.trim_start();
    if rest.is_empty() {
        return Err("usage: cvar <name> [number|\"string\"]".to_string());
    }
    let (name, value_text) = split_once_whitespace(rest).unwrap_or((rest, ""));
    validate_name(name)?;

    let value_text = value_text.trim();
    if value_text.is_empty() {
        return Ok(ConsoleCvarCommand::Get {
            name: name.to_string(),
        });
    }

    let value = if value_text.starts_with('"') {
        CvarValue::String(parse_quoted_string(value_text)?)
    } else if let Ok(value) = value_text.parse::<i64>() {
        CvarValue::Integer(value)
    } else if let Ok(value) = value_text.parse::<f64>() {
        if !value.is_finite() {
            return Err("cvar numbers must be finite".to_string());
        }
        CvarValue::Number(value)
    } else {
        return Err("string cvar values must be wrapped in double quotes".to_string());
    };

    Ok(ConsoleCvarCommand::Set {
        name: name.to_string(),
        value,
    })
}

fn split_once_whitespace(value: &str) -> Option<(&str, &str)> {
    value
        .char_indices()
        .find(|(_, ch)| ch.is_whitespace())
        .map(|(index, _)| (&value[..index], &value[index..]))
}

fn parse_quoted_string(value: &str) -> Result<String, String> {
    let mut chars = value.chars();
    if chars.next() != Some('"') {
        return Err("string cvar values must start with a double quote".to_string());
    }

    let mut output = String::new();
    let mut escaped = false;
    while let Some(ch) = chars.next() {
        if escaped {
            output.push(match ch {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                '"' => '"',
                '\\' => '\\',
                other => other,
            });
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => {
                if chars.as_str().trim().is_empty() {
                    return Ok(output);
                }
                return Err("unexpected text after the closing quote".to_string());
            }
            other => output.push(other),
        }
    }

    Err("unterminated quoted cvar value".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store(slug: &str) -> Rc<RefCell<PluginStore>> {
        let root =
            std::env::temp_dir().join(format!("hebnix-cvar-test-{}-{slug}", std::process::id()));
        Rc::new(RefCell::new(PluginStore::load(&root, slug)))
    }

    #[test]
    fn registry_rejects_duplicate_and_removes_owner_values() {
        let mut registry = CvarRegistry::default();
        let store = test_store("first");
        registry
            .register("shared.value", "first", Rc::clone(&store))
            .unwrap();
        assert_eq!(registry.get("shared.value"), Some(None));
        assert!(
            registry
                .register("shared.value", "second", test_store("second"))
                .is_err()
        );

        registry
            .set("shared.value", CvarValue::Integer(23))
            .unwrap();
        assert_eq!(
            registry.get("shared.value"),
            Some(Some(CvarValue::Integer(23)))
        );

        registry.unregister_owner("second");
        assert!(registry.get("shared.value").is_some());
        registry.unregister_owner("first");
        assert_eq!(registry.get("shared.value"), None);

        registry
            .register("shared.value", "first", Rc::clone(&store))
            .unwrap();
        assert_eq!(
            registry.get("shared.value"),
            Some(Some(CvarValue::Integer(23)))
        );
        assert!(registry.delete("shared.value", "second").is_err());
        registry.delete("shared.value", "first").unwrap();
        assert_eq!(store.borrow().get_cvar("shared.value"), None);
    }

    #[test]
    fn parses_get_and_typed_set_commands() {
        assert_eq!(
            parse_console_command("cvar my_cvar"),
            Ok(ConsoleCvarCommand::Get {
                name: "my_cvar".to_string()
            })
        );
        assert_eq!(
            parse_console_command("cvar my_cvar 23"),
            Ok(ConsoleCvarCommand::Set {
                name: "my_cvar".to_string(),
                value: CvarValue::Integer(23)
            })
        );
        assert_eq!(
            parse_console_command("cvar my_cvar 2.5"),
            Ok(ConsoleCvarCommand::Set {
                name: "my_cvar".to_string(),
                value: CvarValue::Number(2.5)
            })
        );
        assert_eq!(
            parse_console_command(r#"cvar my_cvar "hello world""#),
            Ok(ConsoleCvarCommand::Set {
                name: "my_cvar".to_string(),
                value: CvarValue::String("hello world".to_string())
            })
        );
    }

    #[test]
    fn rejects_invalid_console_values() {
        assert!(parse_console_command("cvar").is_err());
        assert!(parse_console_command("cvar my_cvar unquoted").is_err());
        assert!(parse_console_command("cvar my_cvar \"unterminated").is_err());
        assert!(parse_console_command("cvar my_cvar NaN").is_err());
    }
}
